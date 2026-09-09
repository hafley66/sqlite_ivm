% Independent finite relational predicates for the typed SQL feature fixtures.
% SQL strings are never executed here. Source snapshots become keyed fact deltas.
% Output predicates recompute; this adapter does not claim SQL transaction IVM.
:- use_module(library(http/json)).
:- use_module(library(lists)).
:- dynamic row/3.
:- discontiguous result/2.
:- use_module(library(pairs)).
table(T,R) :- row(T,_,R).
at(R,I,V) :- nth0(I,R,V).
eq(X,Y) :- X\==null,Y\==null,X=:=Y.
lt(X,Y) :- number(X),number(Y),X<Y.
pos(X) :- number(X),X>0.
neg(X) :- number(X),X<0.
condition(eq,A,B) :- at(A,1,K),at(B,1,L),eq(K,L).
condition(residual,A,B) :- condition(eq,A,B),at(A,2,V),at(B,2,W),lt(V,W).
condition(theta,A,B) :- at(A,2,V),at(B,2,W),lt(V,W).
condition(cross,_,_).
nullrow([null,null,null,null]).
pair(_,P,TA,TB,A,B) :- table(TA,A),table(TB,B),condition(P,A,B).
pair(M,P,TA,TB,A,B) :- memberchk(M,[left,full]),table(TA,A),\+ (table(TB,Y),condition(P,A,Y)),nullrow(B).
pair(M,P,TA,TB,A,B) :- memberchk(M,[right,full]),table(TB,B),\+ (table(TA,X),condition(P,X,B)),nullrow(A).
merged(null,K,K) :- !.
merged(K,_,K).
key(K) :- findall(X,(table(a,R),at(R,1,X)),Xs),sort(Xs,Ks),member(K,Ks).
values(K,Vs) :- findall(V,(table(a,R),at(R,1,K),at(R,2,V)),Vs).
nonnull(X) :- X\==null.
sql_sum(Vs,S) :- include(nonnull,Vs,Ns),(Ns=[]->S=null;sum_list(Ns,S)).
sql_avg(Vs,S) :- include(nonnull,Vs,Ns),(Ns=[]->S=null;sum_list(Ns,T),length(Ns,N),S is T/N).
sql_minmax(Vs,Lo,Hi) :- include(nonnull,Vs,Ns),(Ns=[]->Lo=null,Hi=null;min_list(Ns,Lo),max_list(Ns,Hi)).
coalesce(null,0) :- !.
coalesce(V,V).
nullkey(null,first,[0,0]) :- !.
nullkey(null,last,[1,0]) :- !.
nullkey(V,first,[1,V]).
nullkey(V,last,[0,V]).
desckey(null,[1,0]) :- !.
desckey(V,[0,N]) :- N is -V.
ordered(Pairs,Rows) :- keysort(Pairs,Sorted),pairs_values(Sorted,Rows).
top(Rows,Offset,Limit,R) :- nth0(I,Rows,R),I>=Offset,I<Offset+Limit.
partition(K,Order,Rows) :- findall(Key-R,(table(a,R),at(R,1,K),at(R,0,Id),(Order=id->Key=Id;at(R,2,V),nullkey(V,first,VK),Key=VK-Id)),Pairs),ordered(Pairs,Rows).
ranked([Id,K,V,_],N,Rank,Dense) :- partition(K,value,Rows),nth1(N,Rows,[Id,K,V,_]),
 findall(W,(nth1(J,Rows,R),J<N,at(R,2,W),W\==V),Before),length(Before,B),Rank is B+1,sort(Before,Unique),length(Unique,D),Dense is D+1.

result(F,[AI,BI,AK,BK,V,W]) :- member(F,[right_join,full_join,left_residual,full_residual]),
 (F=right_join->Mode=right;F=left_residual->Mode=left;Mode=full),
 (memberchk(F,[left_residual,full_residual])->P=residual;P=eq),
 pair(Mode,P,a,b,[AI,AK,V,_],[BI,BK,W,_]).
result(F,[AI,BI]) :- member(F,[theta_join,cross_join,comma_filter]),(F=theta_join->P=theta;F=cross_join->P=cross;P=residual),pair(inner,P,a,b,[AI,_,_,_],[BI,_,_,_]).
result(using_inner,[K,AI,BI]) :- pair(inner,eq,a,b,[AI,K,_,_],[BI,_,_,_]).
result(using_full,[K,AK,BK,AI,BI]) :- pair(full,eq,a,b,[AI,AK,_,_],[BI,BK,_,_]),merged(AK,BK,K).
result(natural_left,[K,AI,BI]) :- pair(left,eq,a,b,[AI,K,_,_],[BI,_,_,_]).
result(F,[AI,K,V,L,BI,W,N]) :- member(F,[using_star,qualified_star]),pair(full,eq,a,b,[AI,AK,V,L],[BI,BK,W,N]),merged(AK,BK,K).
result(self_outer,[AI,BI,AK,BK]) :- pair(full,eq,a,a,[AI,AK,_,_],[BI,BK,_,_]).
chain(A,B,C) :- pair(left,eq,a,b,A,B),table(c,C),condition(eq,B,C).
chain(A,B,[null,null,null]) :- pair(left,eq,a,b,A,B),\+ (table(c,C),condition(eq,B,C)).
chain(A,B,C) :- table(c,C),\+ (pair(left,eq,a,b,_,Y),condition(eq,Y,C)),nullrow(A),nullrow(B).
result(outer_chain,[AI,BI,CI,AK,BK,CK]) :- chain([AI,AK,_,_],[BI,BK,_,_],[CI,CK,_]).
result(filtered_exists,[Id,K]) :- table(a,A),A=[Id,K,_,_],once((table(b,B),condition(eq,A,B),at(B,2,W),pos(W))).
result(filtered_not_exists,[Id]) :- table(a,A),A=[Id,_,_,_],\+ (table(b,B),condition(residual,A,B)).
result(joined_exists,[Id,K]) :- table(a,A),A=[Id,K,_,_],once((table(b,B),condition(eq,A,B),table(c,C),condition(eq,B,C),at(C,2,Z),pos(Z))).
result(nullable_sum,[K,N,S]) :- key(K),values(K,Vs),length(Vs,N),sql_sum(Vs,S).
grouplabel(K,L) :- findall([X,Y],table(a,[_,X,_,Y]),Bag),sort(Bag,Set),member([K,L],Set).
result(group_only,[K,L]) :- grouplabel(K,L).
result(group_ordinals,[K,L,N]) :- grouplabel(K,L),findall(Id,table(a,[Id,K,_,L]),Is),length(Is,N).
result(aggregate_expression,[K,E]) :- key(K),values(K,Vs),length(Vs,N),sql_sum(Vs,S),coalesce(S,T),E is T+N.
result(having,[K,S]) :- key(K),values(K,Vs),length(Vs,N),N>1,sql_sum(Vs,S),pos(S).
result(having_alias,[K,S]) :- key(K),values(K,Vs),sql_sum(Vs,S),pos(S).
result(empty_having,[0,null]) :- \+table(a,_).
result(aggregate_filter,[K,N,S,M]) :- key(K),values(K,Vs),include(pos,Vs,Ps),include(neg,Vs,Ns),length(Ps,N),sql_sum(Ps,S),sql_avg(Ns,M).
result(distinct_aggs,[K,N,S,M,Lo,Hi]) :- key(K),values(K,Vs),include(nonnull,Vs,Ns),sort(Ns,U),length(U,N),sql_sum(U,S),sql_avg(U,M),sql_minmax(Vs,Lo,Hi).
result(case_cast,[Id,S]) :- table(a,[Id,_,V,L]),(pos(V)->number_string(V,S);V==null->S=L;string_upper(L,S)).
result(like,[Id,Replaced]) :- table(a,[Id,_,_,L]),string_lower(L,Lower),sub_string(Lower,0,1,_,"a"),split_string(L,"a","",Parts),atomics_to_string(Parts,"A",Replaced).
result(cte_columns,[Id,K,V,W]) :- pair(inner,eq,a,b,[Id,K,V,_],[_,_,W,_]).
result(cte_shared,[X,Y]) :- table(a,A),table(a,B),A=[X,_,V,_],B=[Y,_,W,_],pos(V),pos(W),condition(eq,A,B).
result(F,[K]) :- member(F,[distinct_topk,compound_topk]),findall(X,(table(a,[_,X,_,_]);F=compound_topk,table(b,[_,X,_,_])),Ks),sort(Ks,U),findall(Key-X,(member(X,U),nullkey(X,last,Key)),Pairs),ordered(Pairs,Rows),top(Rows,1,3,K).
result(group_topk,[K,S]) :- findall((SK-KK)-[X,T],(key(X),values(X,Vs),sql_sum(Vs,T),desckey(T,SK),nullkey(X,last,KK)),Pairs),ordered(Pairs,Rows),top(Rows,0,2,[K,S]).
result(aggregate_limit_zero,_) :- fail.
ordered_a(Rows) :- findall((VK-Id)-R,(table(a,R),R=[Id,_,V,_],desckey(V,VK)),Pairs),ordered(Pairs,Rows).
result(hidden_order,[Id]) :- ordered_a(Rows),top(Rows,1,3,[Id,_,_,_]).
result(ordinal_order,[Id,V]) :- ordered_a(Rows),top(Rows,0,2,[Id,_,V,_]).
result(bag_topk,[V]) :- findall(Key-X,((table(a,[_,_,X,_]);table(b,[_,_,X,_])),nullkey(X,first,Key)),Pairs),ordered(Pairs,Rows),top(Rows,2,5,V).
result(window_multi,[Id,N,R,D]) :- table(a,A),A=[Id,_,_,_],ranked(A,N,R,D).
result(window_named,[Id,First,Last]) :- table(a,[Id,K,_,_]),partition(K,id,Rows),Rows=[F|_],last(Rows,L),at(F,2,First),at(L,2,Last).
result(window_frame,[Id,S,Previous,Next]) :- table(a,A),A=[Id,K,_,_],partition(K,id,Rows),nth0(I,Rows,A),findall(V,(nth0(J,Rows,R),J>=I-1,J=<I+1,at(R,2,V)),Vs),sql_sum(Vs,S),
 P is I-1,Q is I+1,(P>=0,nth0(P,Rows,PR)->at(PR,2,Previous);Previous= -9),(nth0(Q,Rows,NR)->at(NR,2,Next);Next=null).
result(window_topk,[Id,N]) :- findall((RN-I)-[I,RN],(table(a,A),A=[I,_,_,_],ranked(A,RN,_,_)),Pairs),ordered(Pairs,Rows),top(Rows,0,3,[Id,N]).
result(group_window,[K,S,N]) :- findall((SK-KK)-[X,T],(key(X),values(X,Vs),sql_sum(Vs,T),desckey(T,SK),nullkey(X,first,KK)),Pairs),ordered(Pairs,Rows),nth1(N,Rows,[K,S]),N=<2.
result(outer_aggregate,[K,N,S]) :- key(K),findall([Id,W],pair(left,eq,a,b,[_,K,_,_],[Id,_,W,_]),Pairs),findall(Id,(member([Id,_],Pairs),nonnull(Id)),Ids),length(Ids,N),findall(W,member([_,W],Pairs),Vs),sql_sum(Vs,S).
result(nested_distinct,[K,N]) :- key(K),values(K,Vs),sort(Vs,U),length(U,N).

normalize(V,N) :- (number(V),V=:=round(V)->N is round(V);N=V).
canonical(Rows,Sorted) :- maplist(maplist(normalize),Rows,N),msort(N,Sorted).
sync(T,Rows) :- forall((row(T,Id,Old),\+memberchk(Old,Rows)),retract(row(T,Id,Old))),forall((member(R,Rows),R=[Id|_],\+row(T,Id,R)),assertz(row(T,Id,R))).
verify(F,State) :- forall(member(T,[a,b,c]),(get_dict(T,State.inputs,Rows),sync(T,Rows))),
 findall(R,result(F,R),Actual),canonical(Actual,A),canonical(State.expected,E),
 (A==E->true;throw(error(result_mismatch(F,State.step,A,E),_))),
 forall(member(T,[a,b,c]),(findall(R,table(T,R),Got),get_dict(T,State.inputs,Want),canonical(Got,G),canonical(Want,W),(G==W->true;throw(error(input_mismatch(T),_))))).
run_file(Path) :- setup_call_cleanup(open(Path,read,In),json_read_dict(In,Fixture),close(In)),atom_string(F,Fixture.case.name),retractall(row(_,_,_)),
 maplist(verify(F),Fixture.states),length(Fixture.states,N),json_write_dict(current_output,_{engine:prolog,case:F,status:ok,states:N,input_output_verified:true,algorithm:'SWI bag predicate recomputation; keyed source deltas'},[width(0)]),nl.
main :- current_prolog_flag(argv,[Directory]),directory_files(Directory,Names),sort(Names,Sorted),forall((member(Name,Sorted),file_name_extension(_,json,Name)),(directory_file_path(Directory,Name,Path),run_file(Path))).
:- initialization((catch(main,E,(print_message(error,E),halt(1)))->halt(0);halt(1)),main).
