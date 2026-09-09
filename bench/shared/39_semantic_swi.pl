% Extend the ordinary bag predicate adapter; no extra tabled algorithms.
:- multifile result/2.
:- consult('35_circuit_swi.pl').

key(K) :- setof(Key,Id^V^a(Id,Key,V),Keys),member(K,Keys).
result(minmax,[K,N,Min,Max]) :- key(K),findall(V,a(_,K,V),Vs),length(Vs,N),min_list(Vs,Min),max_list(Vs,Max).
result(count_distinct,[K,N]) :- key(K),findall(V,a(_,K,V),Vs),sort(Vs,Set),length(Set,N).
result(union_set,Row) :- findall([K,V],(a(_,K,V);b(_,K,V)),Bag),sort(Bag,Set),member(Row,Set).
result(except_set,Row) :- findall([K,V],(a(_,K,V),\+b(_,K,V)),Bag),sort(Bag,Set),member(Row,Set).
result(intersect_set,Row) :- findall([K,V],(a(_,K,V),once(b(_,K,V))),Bag),sort(Bag,Set),member(Row,Set).
result(topk,Row) :- findall((NegV-Id)-[K,V],(a(Id,K,V),NegV is -V),Bag),keysort(Bag,Sorted),nth1(N,Sorted,_-Row),N=<3.
result(window_rank,[K,V,N]) :- key(K),findall(V0-Id,a(Id,K,V0),Bag),keysort(Bag,Sorted),nth1(N,Sorted,V-_).
result(subquery,[K,V]) :- a(_,K,V),V>=0.
result(cte,[K,V]) :- a(_,K,V),V>=0.
